; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [2 x i8] c"A\00", align 1
@.str.1 = private unnamed_addr constant [2 x i8] c"B\00", align 1
@.str.2 = private unnamed_addr constant [2 x i8] c"C\00", align 1
@.str.3 = private unnamed_addr constant [2 x i8] c"D\00", align 1
@.str.4 = private unnamed_addr constant [2 x i8] c"F\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_grade(i32 %arg0) {
entry:
  %merge_2 = alloca ptr
  %t0 = add i32 0, 1
  br label %bb1
bb1:
  %t1 = add i32 0, 90
  %t2 = icmp sge i32 %arg0, %t1
  br i1 %t2, label %bb2, label %bb3
bb2:
  store ptr @.str.0, ptr %merge_2
  br label %bb10
bb3:
  %t3 = add i32 0, 80
  %t4 = icmp sge i32 %arg0, %t3
  br i1 %t4, label %bb4, label %bb5
bb4:
  store ptr @.str.1, ptr %merge_2
  br label %bb10
bb5:
  %t5 = add i32 0, 70
  %t6 = icmp sge i32 %arg0, %t5
  br i1 %t6, label %bb6, label %bb7
bb6:
  store ptr @.str.2, ptr %merge_2
  br label %bb10
bb7:
  %t7 = add i32 0, 60
  %t8 = icmp sge i32 %arg0, %t7
  br i1 %t8, label %bb8, label %bb9
bb8:
  store ptr @.str.3, ptr %merge_2
  br label %bb10
bb9:
  store ptr @.str.4, ptr %merge_2
  br label %bb10
bb10:
  %t9 = load ptr, ptr %merge_2
  ret ptr %t9
}

define i32 @main() {
entry:
  %t0 = add i32 0, 95
  %t1 = call ptr @InputKt_grade(i32 %t0)
  call i32 @puts(ptr %t1)
  %t3 = add i32 0, 85
  %t4 = call ptr @InputKt_grade(i32 %t3)
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, 72
  %t7 = call ptr @InputKt_grade(i32 %t6)
  call i32 @puts(ptr %t7)
  %t9 = add i32 0, 61
  %t10 = call ptr @InputKt_grade(i32 %t9)
  call i32 @puts(ptr %t10)
  %t12 = add i32 0, 45
  %t13 = call ptr @InputKt_grade(i32 %t12)
  call i32 @puts(ptr %t13)
  ret i32 0
}

