; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [9 x i8] c"FizzBuzz\00", align 1
@.str.1 = private unnamed_addr constant [5 x i8] c"Fizz\00", align 1
@.str.2 = private unnamed_addr constant [5 x i8] c"Buzz\00", align 1
@.str.3 = private unnamed_addr constant [6 x i8] c"other\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_fizzbuzz(i32 %arg0) {
entry:
  %t0 = add i32 0, 15
  %t1 = srem i32 %arg0, %t0
  %t2 = add i32 0, 0
  %t3 = icmp eq i32 %t1, %t2
  br i1 %t3, label %bb1, label %bb2
bb1:
  ret ptr @.str.0
bb2:
  br label %bb3
bb3:
  %t4 = add i32 0, 3
  %t5 = srem i32 %arg0, %t4
  %t6 = add i32 0, 0
  %t7 = icmp eq i32 %t5, %t6
  br i1 %t7, label %bb4, label %bb5
bb4:
  ret ptr @.str.1
bb5:
  br label %bb6
bb6:
  %t8 = add i32 0, 5
  %t9 = srem i32 %arg0, %t8
  %t10 = add i32 0, 0
  %t11 = icmp eq i32 %t9, %t10
  br i1 %t11, label %bb7, label %bb8
bb7:
  ret ptr @.str.2
bb8:
  br label %bb9
bb9:
  ret ptr @.str.3
}

define i32 @main() {
entry:
  %t0 = add i32 0, 3
  %t1 = call ptr @InputKt_fizzbuzz(i32 %t0)
  call i32 @puts(ptr %t1)
  %t3 = add i32 0, 5
  %t4 = call ptr @InputKt_fizzbuzz(i32 %t3)
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, 15
  %t7 = call ptr @InputKt_fizzbuzz(i32 %t6)
  call i32 @puts(ptr %t7)
  %t9 = add i32 0, 7
  %t10 = call ptr @InputKt_fizzbuzz(i32 %t9)
  call i32 @puts(ptr %t10)
  ret i32 0
}

