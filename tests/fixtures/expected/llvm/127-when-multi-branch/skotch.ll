; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [2 x i8] c"A\00", align 1
@.str.1 = private unnamed_addr constant [2 x i8] c"B\00", align 1
@.str.2 = private unnamed_addr constant [2 x i8] c"C\00", align 1
@.str.3 = private unnamed_addr constant [2 x i8] c"F\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_grade(i32 %arg0) {
entry:
  %merge_2 = alloca ptr
  br label %bb1
bb1:
  %t0 = add i32 0, 10
  %t1 = icmp eq i32 %arg0, %t0
  br i1 %t1, label %bb2, label %bb3
bb2:
  store ptr @.str.0, ptr %merge_2
  br label %bb10
bb3:
  %t2 = add i32 0, 9
  %t3 = icmp eq i32 %arg0, %t2
  br i1 %t3, label %bb4, label %bb5
bb4:
  store ptr @.str.0, ptr %merge_2
  br label %bb10
bb5:
  %t4 = add i32 0, 8
  %t5 = icmp eq i32 %arg0, %t4
  br i1 %t5, label %bb6, label %bb7
bb6:
  store ptr @.str.1, ptr %merge_2
  br label %bb10
bb7:
  %t6 = add i32 0, 7
  %t7 = icmp eq i32 %arg0, %t6
  br i1 %t7, label %bb8, label %bb9
bb8:
  store ptr @.str.2, ptr %merge_2
  br label %bb10
bb9:
  store ptr @.str.3, ptr %merge_2
  br label %bb10
bb10:
  %t8 = load ptr, ptr %merge_2
  ret ptr %t8
}

define i32 @main() {
entry:
  %t0 = add i32 0, 10
  %t1 = call ptr @InputKt_grade(i32 %t0)
  call i32 @puts(ptr %t1)
  %t3 = add i32 0, 9
  %t4 = call ptr @InputKt_grade(i32 %t3)
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, 8
  %t7 = call ptr @InputKt_grade(i32 %t6)
  call i32 @puts(ptr %t7)
  %t9 = add i32 0, 7
  %t10 = call ptr @InputKt_grade(i32 %t9)
  call i32 @puts(ptr %t10)
  %t12 = add i32 0, 3
  %t13 = call ptr @InputKt_grade(i32 %t12)
  call i32 @puts(ptr %t13)
  ret i32 0
}

