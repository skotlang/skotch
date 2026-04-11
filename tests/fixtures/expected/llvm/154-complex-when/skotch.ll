; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [9 x i8] c"FizzBuzz\00", align 1
@.str.1 = private unnamed_addr constant [5 x i8] c"Fizz\00", align 1
@.str.2 = private unnamed_addr constant [5 x i8] c"Buzz\00", align 1
@.str.3 = private unnamed_addr constant [6 x i8] c"other\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_fizzBuzz(i32 %arg0) {
entry:
  %merge_2 = alloca ptr
  %t0 = add i32 0, 1
  br label %bb1
bb1:
  %t1 = add i32 0, 15
  %t2 = srem i32 %arg0, %t1
  %t3 = add i32 0, 0
  %t4 = icmp eq i32 %t2, %t3
  br i1 %t4, label %bb2, label %bb3
bb2:
  store ptr @.str.0, ptr %merge_2
  br label %bb8
bb3:
  %t5 = add i32 0, 3
  %t6 = srem i32 %arg0, %t5
  %t7 = add i32 0, 0
  %t8 = icmp eq i32 %t6, %t7
  br i1 %t8, label %bb4, label %bb5
bb4:
  store ptr @.str.1, ptr %merge_2
  br label %bb8
bb5:
  %t9 = add i32 0, 5
  %t10 = srem i32 %arg0, %t9
  %t11 = add i32 0, 0
  %t12 = icmp eq i32 %t10, %t11
  br i1 %t12, label %bb6, label %bb7
bb6:
  store ptr @.str.2, ptr %merge_2
  br label %bb8
bb7:
  store ptr @.str.3, ptr %merge_2
  br label %bb8
bb8:
  %t13 = load ptr, ptr %merge_2
  ret ptr %t13
}

define i32 @main() {
entry:
  %t0 = add i32 0, 3
  %t1 = call ptr @InputKt_fizzBuzz(i32 %t0)
  call i32 @puts(ptr %t1)
  %t3 = add i32 0, 5
  %t4 = call ptr @InputKt_fizzBuzz(i32 %t3)
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, 15
  %t7 = call ptr @InputKt_fizzBuzz(i32 %t6)
  call i32 @puts(ptr %t7)
  %t9 = add i32 0, 7
  %t10 = call ptr @InputKt_fizzBuzz(i32 %t9)
  call i32 @puts(ptr %t10)
  ret i32 0
}

